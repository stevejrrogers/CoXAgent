You must pick ONE storage design for an audit log with these HARD constraints:
- writes must survive process crash (durability required)
- reads are rare (once a month, by an auditor)
- the box has 512MB RAM total; the log grows ~50GB/year
- no external services allowed (no cloud, no DB server)

Options: (A) in-memory ring buffer with periodic snapshot; (B) append-only file with fsync per batch; (C) embedded LSM database with a large block cache; (D) mmap'd fixed-size arena.

Respond with ONLY JSON: {"choice":"A"|"B"|"C"|"D","why":"...", "rejected":{"A|C|D...":"one-line reason each for the other three"}}.
