### Reflection

When you complete a stage, run Skill(rimz-reflect) on it and end the stage file with a terse `## Reflection`: each entry one fix and where it lands, and none for a smooth stage. A defect in your file that a later stage surfaces is your entry, written when you amend the file. An entry whose fix lives in the worktree goes to the building stage's owner while the run is still open, so it lands as a commit.

Reflect is the last stage of the pipeline. Its owner runs Skill(rimz-reflect) in distill mode, writes `.rimz/team/reflect-notes.md` in the shape that skill gives, and flips to `Done`.
