#!/usr/bin/env python3
"""Generate the Codex skill set from the Claude one.

Codex invokes skills as `$name`, with no plugin namespace and no pre-run shell blocks, so each
Claude skill becomes `whatsai-<name>` and any `!`command`` block becomes an instruction to call
the `whatsai` tool, whose read actions already return the daemon's finished table.
Run without arguments to regenerate; `--check` fails when the generated files are stale.
"""
import pathlib, re, sys

ROOT = pathlib.Path(__file__).resolve().parents[1] / "plugins" / "whatsai"
SOURCE = ROOT / "skills"
TARGET = ROOT / "skills-codex"
ACTION_FOR = {"list": "list", "teams": "teams", "agents": "agents", "inbox": "inbox", "requests": "requests",
              "files": "files", "status": "status", "version": "version"}

def convert(name: str, text: str) -> str:
    front, body = re.match(r"---\n(.*?)\n---\n(.*)", text, re.S).groups()
    front = re.sub(r"^name: .*$", f"name: whatsai-{name}", front, flags=re.M)
    front = "\n".join(l for l in front.split("\n") if not l.startswith("allowed-tools:"))
    action = ACTION_FOR.get(name)
    def replace_block(m):
        return (f"Call the `whatsai` tool with action `{action}` and no arguments; it returns the daemon's "
                f"finished table as text.") if action else ""
    body = re.sub(r"```\n!`[^`]*`\n```", replace_block, body)
    body = body.replace("Show the table below to the user exactly as it is, in a code block",
                        "Show the returned table to the user exactly as it is, in a code block")
    body = body.replace("Do not call any tool to fetch this again; the data was gathered by the daemon before you saw this.",
                        "Call it once.")
    body = body.replace("Claude's read-only skills additionally fetch it with the CLI before the model runs.", "")
    body = body.replace("Unread messages for the Claude agent in this directory:", "Unread messages for this session:")
    body = body.replace("action `inbox`", "action `inbox` with `{\"unread\": true}`") if name == "inbox" else body
    body = re.sub(r"\n{3,}", "\n\n", body).strip() + "\n"
    header = f"<!-- generated from skills/{name}/SKILL.md by scripts/build-codex-skills.py; do not edit -->\n"
    return f"---\n{front}\n---\n{header}\n{body}"

def build() -> dict:
    return {f"whatsai-{d.name}/SKILL.md": convert(d.name, (d / "SKILL.md").read_text())
            for d in sorted(SOURCE.iterdir()) if (d / "SKILL.md").exists()}

def main() -> int:
    wanted = build()
    if "--check" in sys.argv:
        current = {str(p.relative_to(TARGET)): p.read_text() for p in TARGET.rglob("SKILL.md")} if TARGET.exists() else {}
        if current != wanted:
            print("skills-codex is stale; run scripts/build-codex-skills.py", file=sys.stderr)
            return 1
        print(f"skills-codex up to date ({len(wanted)} skills)")
        return 0
    if TARGET.exists():
        for p in TARGET.rglob("SKILL.md"):
            p.unlink()
    for rel, text in wanted.items():
        path = TARGET / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
    print(f"wrote {len(wanted)} Codex skills to {TARGET}")
    return 0

if __name__ == "__main__":
    sys.exit(main())
