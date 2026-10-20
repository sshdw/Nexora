# -*- coding: utf-8 -*-
"""Markdown нұсқасын жасау."""
import re
from content import BLOCKS, SUBTITLE

BOLD_RE = re.compile(r"\*\*(.+?)\*\*")


def md(text):
    return text  # already markdown


def build(path):
    out = []
    out.append("# Химия · 9-сынып конспектісі")
    out.append("")
    out.append(f"**{SUBTITLE}** — оксидтер · негіздер · қышқылдар · тұздар")
    out.append("")
    out.append("> НИШ 9-сынып бағдарламасы: 9.1В Периодтық жүйе заңдылықтары · 9.4 Бейорганикалық қосылыстардың негізгі кластары")
    out.append("")

    for block in BLOCKS:
        kind = block[0]
        if kind == "h1":
            out.append(f"## {block[1]}")
            out.append("")
        elif kind == "h2":
            out.append(f"### {block[1]}")
            out.append("")
        elif kind == "p":
            out.append(block[1])
            out.append("")
        elif kind == "bullets":
            for item in block[1]:
                out.append(f"- {item}")
            out.append("")
        elif kind == "nums":
            for i, item in enumerate(block[1], 1):
                out.append(f"{i}. {item}")
            out.append("")
        elif kind == "note":
            out.append(f"> ⚠️ **{block[1]}**")
            out.append("")
        elif kind == "box":
            out.append("```")
            for line in block[1]:
                out.append(line)
            out.append("```")
            out.append("")
        elif kind == "table":
            headers, rows = block[1], block[2]
            out.append("| " + " | ".join(headers) + " |")
            out.append("|" + "|".join(["---"] * len(headers)) + "|")
            for row in rows:
                out.append("| " + " | ".join(c.replace("|", "/") for c in row) + " |")
            out.append("")
        elif kind == "pagebreak":
            out.append("---")
            out.append("")

    with open(path, "w", encoding="utf-8") as f:
        f.write("\n".join(out))
    print("saved", path)


if __name__ == "__main__":
    build("Химия_9_сынып_конспект.md")
