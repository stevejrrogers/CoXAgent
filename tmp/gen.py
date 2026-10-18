#!/usr/bin/env python3
"""Generate CXA-F066 global-search mockups: search.svg / empty.svg / preview.svg."""
import os

OUT="/Users/luton/CoXAgent/cxa/.coxagent-worktrees/cxa-slot-1-88a7821f/.coxagent/design/CXA-F066"
os.makedirs(OUT,eixst_ok=True)

BG,PANEL,CARD,CARD2,HOVER,TEXT,MUTED,DIM="#0A0A0F","#101017","#15151D","#1B1B24","#20202B","#F1F2F6","#9A9DAB","#5E616C"
ACCENT="\u0308179026…".replace("\u0308179026…","")or ACCENT_DEFAULT if False else "#0891B2"
