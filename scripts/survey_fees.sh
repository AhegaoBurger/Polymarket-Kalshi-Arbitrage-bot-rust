#!/usr/bin/env bash
# One-off survey of Polymarket CLOB /markets response shape per category.
# Used to drive the feeSchedule + bps-to-ppm calibration (PR 1, Task A + B).
#
# Usage:  bash scripts/survey_fees.sh > /tmp/fee_survey.json
#
# Requires: curl, jq
set -euo pipefail

# Gamma category tags. These are what Polymarket publishes on /markets?tag_slug=...
# If a category has no active markets, we fall back gracefully.
CATEGORIES=(sports crypto economics politics tech culture weather finance)

GAMMA="https://gamma-api.polymarket.com/markets"
CLOB="https://clob.polymarket.com/markets"

echo "{"
first=true
for cat in "${CATEGORIES[@]}"; do
  if [ "$first" = true ]; then first=false; else echo ","; fi
  echo "\"$cat\": {"

  # Pick one active market in this category
  sample_json=$(curl -fsS "$GAMMA?tag_slug=$cat&active=true&limit=1" || echo "[]")
  cid=$(echo "$sample_json" | jq -r '.[0].conditionId // .[0].condition_id // empty')

  if [ -z "$cid" ]; then
    echo "\"note\": \"no active markets found for tag_slug=$cat\""
  else
    echo "\"sample_condition_id\": \"$cid\","
    echo "\"gamma_snippet\": $(echo "$sample_json" | jq '.[0] | {id, conditionId, slug, question, category, tags, active, closed}'),"
    echo "\"clob_response\": $(curl -fsS "$CLOB/$cid" || echo 'null')"
  fi
  echo "}"
done
echo "}"
