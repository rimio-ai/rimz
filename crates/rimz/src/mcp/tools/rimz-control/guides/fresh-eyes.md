# Fresh-eyes review

The new session judges the answer your work settled on (a direction, a plan, a design, a diagnosis, a fix) without knowing how the work reached it. When the user names one part of the answer, the review covers that part. The other parts, and the facts that only they rest on, share one line.

The molt note carries:

- The problem the answer is for, as the work now states it.
- The answer as it stands: what it claims, or what is different once it is carried out, and how it is built, as far as that was settled.
- The requirements the answer must meet and the facts it rests on, including a fact that ruled out another answer. Write each as a fact about the system or the work, with no word on who stated it or what it ruled out.

Leave out the rest of the work: who said, proposed, preferred, agreed to, or approved what, the other answers weighed and why they fell, and every argument for this one.

Cite code by symbol, docs by path, and cite each fact's source that way where the new session can read one. When the answer is itself a file (a plan, a diff), cite that file by path as the answer. When the file also records how the work reached the answer, copy the answer from it into the note and leave the file uncited. Every other file written about the work (a notes file, a draft, a child's report) stays uncited too: copy the facts it holds into the note.

The note states facts, and `self_molt`'s `prompt` carries the review's instructions: one sentence that names the note by its path, then this text. When the user narrowed the review, name the part in place of "the answer". Add each instruction the user gave for the review at the end, in their words.

```text
Review the answer it states independently.

Read the code and docs it touches, and anything in the repository that already does the same job. Until you have a verdict, read only these and what the note cites.

Judge four things:
1. Is the problem stated right?
2. Does the answer hold against what you read?
3. What does it cost, and what does it exclude?
4. What did it miss?

Decide whether you would take the answer, change it, or drop it, before you ask the user anything. When the verdict needs a fact that neither the note nor the repository states, assume one and go on.

Assume the user has not read the note. Open your reply with a TL;DR they can follow without it, in plain words:
1. The answer under review, in one or two sentences.
2. Your verdict (take it, change it, or drop it) and the reasons it turns on, three at most.
3. Each assumption you made, and what you would decide if it is false.
4. One sentence naming what else you judged, so they know what to ask for.
```
