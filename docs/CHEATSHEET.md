# Second Brain - ce que le programme traite tout seul (sans IA)

Tu peux écrire n'importe comment : l'IA comprend le texte libre. Mais certaines
choses sont traitées **directement par le programme**, et ça a trois avantages :

- c'est **gratuit** (0 token),
- c'est **instantané et sûr** (même résultat à chaque fois),
- ça **marche même quand l'IA est indisponible** (hors ligne, budget atteint).

Utilise ces syntaxes quand tu veux un résultat **garanti**. Pour tout le reste, écris
librement.

> ⚠️ La reconnaissance des **dates relatives** et des **questions** se fait en
> **anglais** (« next Thursday », « what are my deadlines »). En français, c'est l'IA
> qui s'en charge, ce qui marche mais coûte un appel et peut se tromper. Une date
> complète `2026-11-12` marche dans toutes les langues.

---

## 1. Antisèche express

| Je veux… | J'écris | Résultat garanti |
|---|---|---|
| une tâche | `- [ ] Rendre le TP` | tâche ouverte |
| une tâche avec échéance | `- [ ] Rendre le TP 2026-11-14` | échéance au 14/11 |
| … en relatif | `- [ ] Rendre le TP by Friday` | échéance vendredi prochain |
| une durée | `~2h`, `90m`, `1.5h`, `(45 min)`, `est:2h` | estimation pour le planning |
| une priorité | `!!!` urgent · `!!` important · `!3` normal · `!4` un jour | priorité 1 à 4 |
| un projet | `+SaaS` ou `+mon-projet` | projet créé et tâche rattachée |
| marquer fait | `- [x] Rendre le TP` | tâche terminée |
| annuler | `- [-] Rendre le TP` | tâche annulée |
| viser une tâche précise | `- [x] … #t12` | agit sur la tâche n°12 |
| une décision | `Decision: on garde SQLite` | décision enregistrée |
| une idée | `Idea: appli de fiches de révision` | idée enregistrée |
| une connaissance | `TIL: [[SQLite]] FTS5 classe avec BM25` | connaissance dans la note `SQLite` |
| une tâche sans case | `TODO: appeler la banque` | tâche ouverte |

Exemple complet sur une ligne :

```markdown
- [ ] Finir le dashboard du prototype by Sunday ~3h !! +SaaS
```

→ tâche « Finir le dashboard du prototype », échéance dimanche, 3 h, priorité 2, projet
SaaS. **0 appel IA.**

---

## 2. Les tâches en détail (cases à cocher)

Une ligne qui commence par une case est **toujours** une tâche, traitée sans IA. Le
`- ` devant est facultatif.

| Case | Statut |
|---|---|
| `[ ]` | ouverte |
| `[x]` ou `[X]` | terminée |
| `[-]` | annulée |
| `~~texte barré~~` | le texte barré est nettoyé du titre |

**Ce qui est extrait de la ligne, puis retiré du titre :**

| Élément | Syntaxes reconnues | Notes |
|---|---|---|
| Échéance | voir §4 (dates), `due:2026-11-14` | la **dernière** date de la ligne compte |
| Durée | `2h` `90m` `1.5h` `~45min` `est:2h` `2 hours` `(30 min)` | unités : h, hr, hrs, hour(s), m, min(s), minute(s) ; entre 5 min et 40 h |
| Priorité | `!!!` `!1` `p1` `#p1` `#urgent` → **1** · `!!` `!2` `p2` `#important` → **2** · `!3` `p3` → **3** · `!4` `p4` `#someday` → **4** | par défaut : 3 |
| Projet | `+Nom` (crée le projet), `+mon-projet` → « mon projet » | `[[Nom]]` rattache seulement à un projet **qui existe déjà** |
| Référence | `#t12` | met à jour la tâche n°12 au lieu d'en chercher une par son titre |

**Comment une modification est reconnue (pas de doublon) :**

- Même titre (ou titre très proche, ≥ 75 % de mots communs) → c'est la **même tâche**,
  mise à jour : nouvelle échéance, durée, priorité, terminée.
- Cocher `[x]` une ligne existante → la tâche passe à « terminée ».
- **Une tâche terminée n'est jamais réactivée** par un vieux texte. Si un ancien journal
  contient encore `- [ ] …`, c'est ignoré et signalé. Pour la rouvrir :
  `sb task reopen <id>`.

---

## 3. Préfixes de ligne

En début de ligne (ou après `- `), insensibles à la casse :

| Préfixe | Devient |
|---|---|
| `TODO:` | tâche (mêmes règles de durée, date, priorité qu'une case) |
| `Decision:` / `Decided:` | décision (visible dans la note du projet et dans les synthèses) |
| `Idea:` | idée (note dans `knowledge/`) |
| `TIL:` / `Learned:` | connaissance ; le concept est le premier `[[lien]]` de la ligne, sinon les 4 premiers mots |

Ces lignes ne passent **pas** par l'IA : elles ne reçoivent donc pas de thème
automatiquement. L'agent `sb connect` les rangera ensuite dans l'arbre.

---

## 4. Dates reconnues par le programme (anglais)

Toutes les dates relatives se calculent **par rapport à la date du journal** (nom du
fichier `2026-11-07.md` ou `date:` dans le frontmatter), pas par rapport au jour où
le programme tourne.

| Tu écris | Signifie |
|---|---|
| `2026-11-14` | ce jour-là (marche aussi en français) |
| `today`, `tonight` | le jour du journal |
| `tomorrow` / `yesterday` | J+1 / J−1 |
| `Thursday`, `on Thursday`, `by Thursday`, `next Thursday` | le **prochain** jeudi (jamais le jour même) |
| `this Saturday` (écrit un samedi) | aujourd'hui |
| `before Sunday` | **la veille** : samedi |
| `last Monday` | le lundi précédent |
| `next week` | lundi prochain |
| `this weekend` | samedi |
| `end of the week` | dimanche |
| `in 3 days`, `in two weeks`, `in a couple of days` | J+3, J+14, J+2 |
| `Nov 20`, `November 20th`, `20 November`, `3rd of January`, `Nov 20 2027` | date du calendrier ; une date passée de plus de 2 mois → l'année suivante |

Les abréviations (`Mon`, `Tue`, `Wed`, `Thu`, `Fri`, `Sat`, `Sun`) ne comptent que si
elles ont une **majuscule** ou un mot comme `by`/`on`/`before` devant. Sinon « I sat
down » serait lu comme samedi.

Les dates en anglais trouvées dans ton texte libre sont **aussi données à l'IA comme
indices déjà calculés**, ce qui évite qu'elle se trompe de jour.

---

## 5. Comment ta note est découpée (et pourquoi ça compte)

Le programme découpe chaque note en **blocs**. Un bloc est l'unité qui est « nouvelle »
ou « déjà lue ».

| Dans ta note | Effet |
|---|---|
| Ligne vide | sépare deux blocs |
| Paragraphe (lignes consécutives) | un bloc |
| `# Titre`, `## Titre` | pas un bloc : sert de **contexte** (« sous le titre X ») envoyé à l'IA |
| Liste numérotée `1.` `1)` `1.Texte` `1️⃣`, même avec des lignes vides entre les items | **un seul bloc** si la numérotation se suit |
| Puces `- ` collées les unes aux autres | **un seul bloc** |
| Ligne qui finit par `:` suivie d'une liste | rattachée à la liste |
| Case à cocher | **toujours son propre bloc**, jamais fusionnée |
| Bloc de code (` ``` ` ou `~~~`) | **ignoré** |
| `<!-- commentaire -->` | ignoré |
| Frontmatter `---` en tête | lu pour `date:`, pas analysé |
| Fichiers commençant par `.` | ignorés |

**Ce qui coûte (ou pas) :**

- Fichier inchangé → **0 travail** (empreinte du fichier).
- Re-sauvegarder sans rien changer, ou juste reformater les espaces et retours à la
  ligne → **0 appel** : l'empreinte ignore les espaces.
- Ajouter un paragraphe → **seul ce paragraphe** part à l'IA.
- Modifier un mot dans un long paragraphe → **tout le paragraphe** repart.
  👉 Saute des lignes entre les idées distinctes.
- Supprimer du texte → l'historique **n'est pas effacé** : ce qui avait été appris
  reste, avec sa source.
- Ton journal n'est **jamais modifié** par le programme.

---

## 6. Sans IA (`sb ingest --offline`, ou IA indisponible)

Le programme détecte quand même les engagements écrits en **anglais** :

- `I need to`, `I have to`, `I must`, `I should`, `I've got to`, `remember to`,
  `don't forget to` → tâche, avec la date relative si elle est présente.
- `… and …` sépare plusieurs tâches : « I need to revise maths before Thursday **and**
  finish the prototype by Sunday » → 2 tâches.
- Avec `maybe`, `might`, `perhaps`, `should`, `could`, `someday` dans le paragraphe →
  tâche **proposée** (à confirmer : `sb task confirm <id>`), pas un engagement.

Quand l'IA est indisponible ou que le budget du jour est atteint, les paragraphes
restent **en attente** et sont traités plus tard. Rien n'est perdu.

---

## 7. Les questions sans IA (`sb ask`)

Si ta question contient ces mots (en anglais), la réponse vient **directement de la
base**, avec 0 appel :

| Mots | Réponse |
|---|---|
| `overdue`, `late`, `missed` | tâches en retard |
| `deadline`, `due`, `exam` (sauf « what is », « explain », « how ») | échéances des 30 prochains jours |
| `today`, `tomorrow`, `schedule`, `plan for`, `agenda`, `free time`, `available` (sauf « why », « explain ») | agenda du jour ou du lendemain |
| `my tasks`, `todo`, `to do`, `what should i do`, `what's next`, `next task` | tes 10 tâches prioritaires |

Toute autre question part à l'IA, avec un contexte limité : profil, tâches et
souvenirs pertinents.

**Commandes toujours sans IA :** `sb tasks`, `sb today`, `sb plan`, `sb search`,
`sb themes`, `sb memory`, `sb log`, `sb undo`, `sb unlink "A" "B"` (retirer un lien), `sb dash` (tableau de bord dans le terminal), `sb event "…"` (ajouter un événement à l'agenda en langage naturel), `sb graph`, `sb stats`, `sb doctor`,
`sb briefing` (sans `--llm`).

---

## 8. Priorités, planning et alertes (100 % programme)

**Score de priorité** d'une tâche (plus haut = plus pressant) :

- **importance** : p1 = 40, p2 = 30, p3 = 20, p4 = 10 ;
- **urgence** : jusqu'à +56 quand l'échéance approche, +60 si elle est dépassée ;
- **pression** : bonus si le travail restant ne tient pas dans les jours qui restent ;
- **−15** pour une tâche seulement « proposée ».

**Planificateur** (`sb plan`, réglages dans la section `[schedule]` de `config.toml`) :

- Il place le travail dans tes `work_hours`, **jamais** dans `protected` ni sur un
  créneau occupé de Google Agenda.
- Ordre : l'échéance la plus proche d'abord, puis le score.
- Blocs entre `min_block_minutes` et `max_block_minutes`, avec une pause entre deux
  blocs, au maximum `max_focus_minutes_per_day` par jour et 2 blocs de la même tâche
  par jour.
- Une tâche sans durée prend `default_estimate_minutes` (60), et c'est **signalé**.
  👉 Mets toujours une durée (`~2h`) aux tâches importantes.
- Les tâches **proposées** et les **échéances fixes** (examens) ne sont pas
  planifiées.
- La replanification est **incrémentale** : seules les sessions invalides
  (conflit, tâche finie, nouvelle échéance) bougent.
- Une session passée et non marquée faite est considérée comme **manquée**, et son
  travail est replanifié. Marque-la faite avec `sb session <id> done`.

**Briefing et alertes :**

- Alerte « à faire aujourd'hui / demain / dans 2 jours », « en retard », « à risque ».
  **Chaque alerte n'est montrée qu'une fois** tant que rien ne change.
- Suggestions automatiques :
  - examen dans ≤ 10 jours sans tâche de préparation qui partage un mot du sujet ;
  - journée surchargée ;
  - tâches sans durée ;
  - planning à valider.
- Un rappel de connaissance n'est pas répété avant 7 jours.

---

## 9. Ce que fait l'IA (pour savoir quoi laisser libre)

Tout ce qui demande de **comprendre** :

- extraire les connaissances, idées, ressources « à explorer », décisions,
  événements, objectifs, projets, préférences ;
- **ranger dans l'arbre** grand thème › sous-thème, en réutilisant ce qui existe ;
- **créer les liens** entre éléments, avec leur raison (`sb connect` le refait thème
  par thème) ;
- distinguer ce que **tu** penses de ce que tu as **copié** (affirmations d'articles →
  « opinion / à vérifier ») ;
- les synthèses jour, semaine et mois (`sb synth`) ;
- les vraies questions (`sb ask`).

Le programme **vérifie tout** ce que l'IA renvoie : noms inexistants rejetés, dates
impossibles refusées, numéros de paragraphe corrigés. Chaque action est journalisée
et annulable (`sb log`, puis `sb undo <id>`).

---

## 10. Bonnes habitudes d'écriture

1. **Une idée = un paragraphe**, séparé par une ligne vide. Ça coûte moins cher quand
   tu modifies et ça donne des sources plus précises.
2. **Les engagements en cases à cocher** avec durée et date :
   `- [ ] … 2026-11-14 ~2h !!`. Résultat garanti, gratuit, et le planning est juste.
3. **Une date complète** (`2026-11-14`) quand tu écris en français.
4. **`+Projet`** sur les tâches pour qu'elles apparaissent dans la note du projet.
5. **Les listes en un bloc** : numérotation qui se suit, ou puces collées, avec une
   ligne d'intro qui finit par `:`. Une liste = une idée complète.
6. **Les liens seuls sur une ligne** (`https://…`, `crt.sh`) deviennent des
   ressources à explorer. Ajoute un mot de contexte (« pour les labs SIEM ») et le
   rangement sera meilleur.
7. **`[[Nom]]`** pour forcer un rattachement à une note qui existe.
8. **Ne mets pas d'instructions pour l'IA** dans tes notes en espérant qu'elle les
   suive : elles sont traitées comme des données (sécurité). Un prompt collé est
   simplement sauvegardé comme ressource.
9. **Corrige plutôt que supprimer** : cocher `[x]`, changer la date, ou
   `sb memory --retract <id>` pour un souvenir faux.
10. **Écris dans le bloc libre des notes générées** : dans `knowledge/` et
    `projects/`, tout ce qui est hors du bloc `<!-- sb:begin … sb:end -->` est à toi
    et n'est jamais écrasé.
