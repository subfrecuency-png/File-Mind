# Research sandbox

Python-only experiments that never ship: classifier prototypes, embedding model comparisons, eval sets.

```sh
python3 -m venv .venv && source .venv/bin/activate
pip install -r requirements.txt
```

Eval sets live in `research/data/` (git-ignored — they are built from personal file trees and stay local).

- `classification/` — 2 k labelled files → target ≥ 85 % top-1 (Phase 4)
- `search/` — 200 natural-language queries with gold answers → ≥ 80 % top-5 (Phase 7)
- `projects/` — 10 hand-labelled dogfood trees → ≥ 70 % recognised (Phase 5)
