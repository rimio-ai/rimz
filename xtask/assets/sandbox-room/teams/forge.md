---
leader: coder
stages: [Build, Review, Ship]
roles:
  - role: coder
    agent: worker
    owns: [Build, Ship]
  - role: reviewer
    agent: worker
    owns: [Review]
---
Complete the work.
