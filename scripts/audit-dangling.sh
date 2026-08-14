#!/usr/bin/env bash
# audit-dangling.sh — find code that is written but never reached.
#
# WHY THIS EXISTS
#   An agent writing new code does not reliably read the code that already
#   exists. That produces two artifacts nobody can account for at audit time:
#   functions nothing calls, and second implementations of something already
#   implemented elsewhere. Both compile. Both pass tests. Neither is reachable
#   from the product.
#
#   This finds the MECHANICAL half only. It cannot find a logical error — code
#   that runs, is reached, and is wrong. That needs adversarial review; see
#   the `span_names_the_property` guard in crates/ingest/src/repair.rs for a
#   case where the code worked, the tests passed, the stored quote was real,
#   and the fact was still wrong.
#
# USAGE   ./scripts/audit-dangling.sh [crate-dir ...]      (default: all crates)
# EXIT    0 = clean, 1 = findings

set -uo pipefail
cd "$(dirname "$0")/.." || exit 2

# `targets` limits where we look for DEFINITIONS. Callers are always searched
# across the whole workspace: a function defined in `ingest` and called from
# `cli` is not an orphan, and scoping the caller search to one crate would
# report every cross-crate entry point as dangling.
targets=("$@")
[ ${#targets[@]} -eq 0 ] && targets=(crates)
readonly CALLER_SCOPE=crates

# Findings are counted in a FILE, not a variable: the loops below run on the
# right-hand side of a pipe, so they execute in a subshell and any variable
# they increment is discarded when it exits. A count kept in a shell variable
# here would report "clean" no matter what was found.
count_file=$(mktemp)
trap 'rm -f "$count_file"' EXIT
printf 0 > "$count_file"

# --- 1. orphans: production fns nothing calls -------------------------------
# Skips everything below #[cfg(test)]. Skips names referenced from an attribute
# string (serde's `skip_serializing_if = "is_true"` is a real call site that no
# grep for `is_true(` will ever see). Skips trait method bodies, whose callers
# go through the trait, not the name.
echo "── orphaned functions ──"
while IFS= read -r file; do
  cut=$(grep -n '^#\[cfg(test)\]' "$file" | head -1 | cut -d: -f1)
  cut=${cut:-999999}
  awk -v c="$cut" '
    NR>=c { exit }
    /^[[:space:]]*#\[(tokio::)?test\]/ { harness=1; next }
    /^[[:space:]]*(pub |pub\([a-z]+\) )?(async )?fn [a-z_]/ {
      if (harness) { harness=0; next }
      if (match($0, /fn [a-z_][a-z0-9_]*/))
        print substr($0, RSTART+3, RLENGTH-3)
    }' "$file"
done < <(find "${targets[@]}" -name '*.rs' -not -path '*/target/*') | sort -u |
while read -r fn; do
  [ -z "$fn" ] && continue
  # a call, a method call, or a bare path reference that is not the definition
  calls=$(grep -rn --include='*.rs' -e "\b${fn}(" -e "\.${fn}(" -e "\"${fn}\"" \
            "$CALLER_SCOPE" 2>/dev/null | grep -vc "fn ${fn}\b")
  if [ "$calls" -eq 0 ]; then
    # last check: a trait method is reached through the trait, not by name
    if ! grep -rqn --include='*.rs' "fn ${fn}\b.*;" "$CALLER_SCOPE" 2>/dev/null; then
      loc=$(grep -rn --include='*.rs' "fn ${fn}\b" "${targets[@]}" | head -1)
      echo "  ORPHAN  ${fn}  ${loc%%:*}"
      printf '%s' "$(( $(cat "$count_file") + 1 ))" > "$count_file"
    fi
  fi
done

# --- 2. duplicate free functions --------------------------------------------
# Same name defined as a FREE function (column-0 fn, no enclosing impl/trait)
# in more than one file = one of them is a reimplementation of the other.
echo "── duplicate free functions ──"
while IFS= read -r file; do
  cut=$(grep -n '^#\[cfg(test)\]' "$file" | head -1 | cut -d: -f1)
  cut=${cut:-999999}
  awk -v c="$cut" -v f="$file" '
    NR>=c { exit }
    /^(impl|trait|pub trait)[ <]/ { depth=1 }
    depth==1 && /^}/            { depth=0 }
    depth==0 && /^(pub )?(pub\([a-z]+\) )?(async )?fn [a-z_]/ {
      if (match($0, /fn [a-z_][a-z0-9_]*/))
        print substr($0, RSTART+3, RLENGTH-3) "\t" f
    }' "$file"
done < <(find "${targets[@]}" -name '*.rs' -not -path '*/target/*') |
sort -u | awk -F'\t' '{n[$1]++; where[$1]=where[$1]" "$2} END {
  for (k in n) if (n[k] > 1) print "  DUPLICATE  " k where[k]
}'

echo
findings=$(cat "$count_file")
if [ "$findings" -eq 0 ]; then
  echo "clean — every production function is reachable"
else
  echo "$findings dangling function(s) — each must be wired up or deleted"
fi
exit $(( findings > 0 ))
