#!/usr/bin/env bash
# Derive data/cues.txt — the discriminating collocates that settle a real-word
# confusion — from the Google Books Ngrams corpus.
#
# The output is committed, so this runs only when the table is being rebuilt.
# It takes roughly twenty minutes and streams about 39 GB; nothing is kept on
# disk but the counts for words we care about.
#
# WHY DERIVE RATHER THAN CURATE
#
# The first version of the cue table was written by hand, and hand-writing it
# is how a wrong cue gets in: the bar is that a cue selects one member of a
# confusion set and the others essentially *never* take it, and no one can hold
# that judgement steady across fifty pairs. A corpus can. The exclusivity test
# below is the same sentence, in arithmetic.
#
# WHY THIS CORPUS
#
# Google Books Ngrams is published under CC BY 3.0 — attribution and nothing
# else, which is the only large n-gram set that is unambiguously usable here.
# Peter Norvig's bigram counts are more convenient and carry no license on the
# data at all; the underlying corpus came through the LDC. They are out.
#
# WHAT IS AND ISN'T REACHABLE
#
# The 2-gram files are partitioned by the first two characters of the *first*
# word. So for a confusable W:
#
#   "W Y"  (after-cues)  lives in the shard for W          — always fetched
#   "X W"  (before-cues) lives in the shard for X          — only if X's shard is
#
# After-cues are therefore complete. Before-cues are complete only for cues
# beginning with a prefix in EXTRA_PREFIXES below. Fetching every shard would
# make them complete too, at something over 200 GB, which is not worth it: an
# incomplete cue table is correct, just quieter. Silence is the right output
# for a context nobody has evidence about.
set -euo pipefail

YEAR_MIN="${YEAR_MIN:-1960}"     # Books skews old; modern usage is the target.
MIN_COUNT="${MIN_COUNT:-2000}"   # Below this the corpus isn't really speaking.
RATIO="${RATIO:-40}"             # How much the winner must beat the runner-up.
MAX_PER_WORD="${MAX_PER_WORD:-24}"
# Low on purpose. Six parallel multi-gigabyte streams got connection resets
# from the CDN, and a reset mid-stream is the dangerous failure here, not the
# slow one — see the completeness check below.
JOBS="${JOBS:-6}"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$ROOT/data/cues.txt"
WORK="${CUE_WORK:-$(mktemp -d)}"
BASE="http://storage.googleapis.com/books/ngrams/books/googlebooks-eng-all-2gram-20120701"

# Prefixes worth fetching for before-cues. Chosen so the corpus can be checked
# against the cues that were originally written by hand — if it cannot
# rediscover `apart from` and `rather than`, the thresholds are wrong.
EXTRA_PREFIXES="${EXTRA_PREFIXES:-ap ra ev aw fa mo le be ot gr}"

mkdir -p "$WORK" "$ROOT/data"

# The confusion sets come from the source, not a copy. A cue for a word that is
# no longer confusable is dead weight, and cue.rs asserts against exactly that.
python3 - "$ROOT/src/ngram.rs" >"$WORK/sets.txt" <<'PY'
import re, sys
src = open(sys.argv[1]).read()
body = src[src.index("CONFUSION_SETS"):]
body = body[body.index("&["):body.index("];")]
for line in re.findall(r'&\[((?:\s*"[^"]+"\s*,?)+)\]', body):
    words = re.findall(r'"([^"]+)"', line)
    if len(words) > 1:
        print(" ".join(words))
PY

cut -d' ' -f1- "$WORK/sets.txt" | tr ' ' '\n' | sort -u >"$WORK/words.txt"
prefixes=$(cut -c1-2 "$WORK/words.txt" | sort -u | tr '\n' ' ')
all_prefixes=$(echo "$prefixes $EXTRA_PREFIXES" | tr ' ' '\n' | grep -E '^[a-z]{2}$' | sort -u)

echo "confusables: $(wc -l <"$WORK/words.txt" | tr -d ' ')"
echo "shards:      $(echo "$all_prefixes" | wc -l | tr -d ' ')"

# One pass per shard, in parallel. Each keeps only the bigrams where one side
# is a confusable, which is a few thousand lines out of tens of millions.
fetch() {
    local prefix="$1"
    local dest="$WORK/counts-$prefix.tsv"
    [ -s "$dest" ] && return 0   # Idempotent: a re-run resumes.
    # Download to a file, THEN decompress. Piping curl straight into gzcat is
    # the obvious shape and it is wrong with retries on: a retry restarts the
    # transfer from byte zero, so the decompressor — which cannot rewind — sees
    # a fresh gzip header partway through a stream it was already reading,
    # exits, and curl dies writing to a closed pipe. That failure looked like
    # corrupt data rather than a retry, which is a bad half hour.
    #
    # --retry-all-errors because the default retries HTTP statuses only, and
    # the failures that actually happen over tens of gigabytes are DNS
    # resolution and connection resets.
    local raw="$WORK/$prefix.gz"
    curl -fsSL --retry 8 --retry-delay 5 --retry-all-errors --connect-timeout 30 \
        -C - -o "$raw" "$BASE-$prefix.gz" || {
        echo "  $prefix DOWNLOAD FAILED" >&2
        rm -f "$raw"
        return 1
    }

    set -o pipefail
    # grep before awk. Only about one line in 300 mentions a confusable, and
    # a fixed-string matcher rejects the rest far faster than awk can split
    # and lowercase them — awk, not the network, was the bottleneck on the
    # first full run. grep is case-insensitive because the corpus is mixed
    # case and awk lowercases afterwards anyway.
    gzcat "$raw" | grep -iFf "$WORK/words.txt" | awk -F'\t' -v ymin="$YEAR_MIN" '
        NR == FNR { want[$0] = 1; next }
        $2 < ymin { next }
        {
            # Underscores mark part-of-speech variants ("effect_NOUN"), which
            # would double-count the same surface bigram.
            if (index($1, "_")) next
            n = split($1, w, " ")
            if (n != 2) next
            a = tolower(w[1]); b = tolower(w[2])
            if (a in want || b in want) total[a " " b] += $3
        }
        END { for (g in total) print g "\t" total[g] }
    ' "$WORK/words.txt" - >"$dest.tmp" || {
        # A shard with no confusable at all would exit 1 from grep, but every
        # prefix here holds one by construction, so this really is a failure.
        echo "  $prefix DECOMPRESS FAILED" >&2
        rm -f "$dest.tmp" "$raw"
        return 1
    }
    rm -f "$raw"
    if [ ! -s "$dest.tmp" ]; then
        echo "  $prefix EMPTY (treated as failure)" >&2
        rm -f "$dest.tmp"
        return 1
    fi
    mv "$dest.tmp" "$dest"
    echo "  $prefix done ($(wc -l <"$dest" | tr -d ' ') bigrams)"
}
export -f fetch
export WORK BASE YEAR_MIN

echo "$all_prefixes" | xargs -P "$JOBS" -I{} bash -c 'fetch "$@"' _ {} || true

# A missing shard is FATAL, not a warning. This is the subtle part, and it
# cost a full run to learn:
#
# A cue is emitted when one member of a confusion set takes a context word and
# the others do not. Both counts come from the *same* shard — "and there" and
# "and their" both live under "an" — so a shard that is present gives an honest
# comparison, and a shard that is missing gives no cue at all. Safe either way.
#
# A shard that is *partial* is neither. It reports "and there" at twenty-one
# million and "and their" at seven hundred, and the ratio test then certifies
# `and` as an exclusive cue for `there`. The table would tell the checker to
# flag "and their" — correct English — as a mistake. Partial input does not
# produce a smaller table here; it produces a confidently wrong one.
#
# So: fail loudly, keep whatever completed, and make the operator re-run.
missing=$(comm -23 <(echo "$all_prefixes") \
    <(find "$WORK" -name 'counts-??.tsv' -size +0 2>/dev/null |
        sed -n 's/.*counts-\(..\)\.tsv$/\1/p' | sort))
if [ -n "$missing" ]; then
    echo "" >&2
    echo "FATAL: no data for: $(echo "$missing" | tr '\n' ' ')" >&2
    echo "" >&2
    echo "Refusing to derive cues from partial counts: a shard that is" >&2
    echo "missing makes the words it holds look exclusive when they are not," >&2
    echo "and the table would flag correct English." >&2
    echo "" >&2
    echo "Re-run to retry — completed shards are reused." >&2
    exit 1
fi

cat "$WORK"/counts-*.tsv >"$WORK/all.tsv"
echo "merged $(wc -l <"$WORK/all.tsv" | tr -d ' ') bigrams"

# Turn counts into cues.
python3 "$ROOT/script/derive-cues.py" "$WORK/all.tsv" "$WORK/sets.txt" \
    "$ROOT/data/wordlist.txt" "$OUT" "$MIN_COUNT" "$RATIO" "$MAX_PER_WORD"
