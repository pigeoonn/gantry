#!/usr/bin/env python3
"""Add phrases to the interface languages (the ten of `src/i18n.rs`).

    python3 tools/i18n_add.py rows.py

`rows.py` defines ROWS, a list of (English, Romanian, Italian, French, Spanish, Russian); it may also define COLUMNS, the
languages of the columns after the English one (for example ["de", "pt", "pl", "tr"]). In a translation `{}`
placeholders are written `{1}`, `{2}` (by position). A plural phrase has the English "other" form as its key and a
list of forms per language (Romanian and Russian have three, the others two). Existing phrases are replaced.
`cargo test` then says whether anything is still missing.
"""
import json
import runpy
import sys
from pathlib import Path

LANGS = ["ro", "it", "fr", "es", "ru", "de", "pt", "pl", "tr"]
DEFAULT_COLUMNS = ["ro", "it", "fr", "es", "ru"]
DIR = Path(__file__).resolve().parent.parent / "src" / "i18n"


def dump_list(items):
    return "[\n" + ",\n".join(json.dumps(i, ensure_ascii=False) for i in items) + "\n]"


def dump_dict(d):
    return "{\n" + ",\n".join(json.dumps(k, ensure_ascii=False) + ": " + json.dumps(v, ensure_ascii=False) for k, v in d.items()) + "\n}"


def main(path):
    ns = runpy.run_path(path)
    rows = ns["ROWS"]
    columns = ns.get("COLUMNS", DEFAULT_COLUMNS)
    en = json.loads((DIR / "en.json").read_text())
    dicts = {}
    for l in LANGS:
        f = DIR / f"{l}.json"
        dicts[l] = json.loads(f.read_text()) if f.exists() else {}
    for row in rows:
        assert len(row) == 1 + len(columns), row
        key = row[0]
        if key not in en:
            en.append(key)
        for l, tr in zip(columns, row[1:]):
            dicts[l][key] = tr
    en.sort(key=lambda k: k.lower())
    (DIR / "en.json").write_text(dump_list(en))
    for l in LANGS:
        d = dicts[l]
        if not d and not (DIR / f"{l}.json").exists():
            continue
        (DIR / f"{l}.json").write_text(dump_dict({k: d[k] for k in en if k in d}))
    print(f"{len(rows)} phrases written; {len(en)} in total")


if __name__ == "__main__":
    main(sys.argv[1])
