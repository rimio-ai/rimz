---
leader: coder
stages: [Build, Review]
roles:
  - role: coder
    agent: worker
    owns: [Build]
  - role: reviewer
    agent: worker
    owns: [Review]
---
Complete the work.
