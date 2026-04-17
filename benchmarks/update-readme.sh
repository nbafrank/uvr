#!/usr/bin/env bash
#
# Reads benchmarks/bench-results.json and patches the README.md benchmark table.
# Run after bench.sh completes:
#   bash benchmarks/update-readme.sh
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
JSON="$SCRIPT_DIR/bench-results.json"
README="$ROOT_DIR/README.md"

if [ ! -f "$JSON" ]; then
    echo "error: $JSON not found. Run bench.sh first." >&2
    exit 1
fi

python3 - "$JSON" "$README" << 'PYEOF'
import json, re, sys

json_path = sys.argv[1]
readme_path = sys.argv[2]

with open(json_path) as f:
    data = json.load(f)

arch = data["meta"]["arch"]
r_ver = data["meta"]["r_version"].replace("R version ", "")
r_short = ".".join(r_ver.split(".")[:2])
runs = data["meta"]["runs_per_tier"]

# Collect warm-tier results
warm = {}
for r in data["results"]:
    if r["tier"] != "warm":
        continue
    scenario = r["scenario"]
    tool = r["tool"]
    if scenario not in warm:
        warm[scenario] = {"packages": r["packages"]}
    warm[scenario][tool] = r["median"]

# Determine which tools are present
all_tools = []
for tool in ["uvr", "renv", "install.packages", "pak"]:
    if any(tool in warm[s] for s in warm):
        all_tools.append(tool)

tool_names = {
    "uvr": "uvr sync",
    "renv": "renv",
    "install.packages": "install.packages",
    "pak": "pak",
}

scenarios = ["jsonlite", "ggplot2", "tidyverse"]

def find_fastest(scenario_data):
    best_tool = None
    best_time = float("inf")
    for tool in all_tools:
        t = scenario_data.get(tool)
        if t is not None and t < best_time:
            best_time = t
            best_tool = tool
    return best_tool

# Build the replacement block
lines = []
arch_label = "Apple Silicon (arm64)" if arch == "arm64" else arch
lines.append(f"Install wall time (empty library, index caches warm). All tools use P3M as CRAN mirror. Median of {runs} runs on {arch_label}, R {r_short}.")
lines.append("")

# Table header
cols = ["Scenario", "Packages"] + [tool_names[t] for t in all_tools]
lines.append("| " + " | ".join(cols) + " |")
lines.append("|" + "|".join("-" * (len(c) + 2) for c in cols) + "|")

for scenario in scenarios:
    if scenario not in warm:
        continue
    s = warm[scenario]
    fastest = find_fastest(s)
    npkg = s["packages"]
    row = f"| {scenario:<9} | {npkg:<8}"
    for tool in all_tools:
        t = s.get(tool)
        if t is None:
            cell = "n/a"
        else:
            cell = f"{t}s"
            if tool == fastest:
                cell = f"**{cell}**"
        col_width = len(tool_names[tool]) + 2
        row += f" | {cell:<{col_width}}"
    row += " |"
    lines.append(row)

block = "\n".join(lines)

# Read and patch README
with open(readme_path) as f:
    content = f.read()

pattern = r'<!-- BENCH:START.*?-->.*?<!-- BENCH:END -->'
replacement = f'<!-- BENCH:START - auto-updated by benchmarks/update-readme.sh -->\n{block}\n<!-- BENCH:END -->'

new_content, count = re.subn(pattern, replacement, content, flags=re.DOTALL)
if count == 0:
    print("error: BENCH:START/END markers not found in README.md", file=sys.stderr)
    sys.exit(1)

with open(readme_path, "w") as f:
    f.write(new_content)

print(f"Updated README.md benchmark table ({count} replacement)")
PYEOF
